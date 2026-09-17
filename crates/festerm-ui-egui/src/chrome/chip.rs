//! Chip measurement and painting: a chip's natural (content-driven) width,
//! its layout inside the allocated row, and the two-line primary/secondary
//! paint (status dot, identity, close control). Split out of `chrome.rs`;
//! see that module's docs for the presentation contract chips live under.

use super::*;

/// Ephemeral, UI-only rename buffer key: whether `chip_id` currently has an
/// in-progress rename edit, and its current (uncommitted) text. Not part of
/// `ChipViewModel` because this module is pure presentation
/// (`docs/gui-design.md`); the caller only ever sees a committed
/// `ChromeAction::Rename`.
fn rename_buffer_id(id: ChipId) -> Id {
    Id::new("chrome_chip_rename").with(id.0)
}

/// Measures a chip's true content-driven width directly from its text,
/// independent of whatever rect it's actually painted into.
///
/// This exists because `paint_chip` used to be measured *after* painting it
/// into a rect the caller already chose (`content_response.rect`, which is
/// just `ui.max_rect()` - the imposed rect, echoed straight back, not the
/// label's actual desired size). That made a chip's cached "natural" width
/// a no-op feedback loop: whatever width it was given is exactly the width
/// it reported back, so a chip could never discover it wanted to be wider
/// than whatever it happened to start at - including its very first frame,
/// which defaulted to the minimum before a real width was ever cached.
/// In practice every chip appeared permanently stuck at that minimum
/// and the Chrome-style "shrink before scroll" row always had nothing to
/// shrink from. Measuring the text directly (mirroring `paint_chip`'s own
/// insets/spacing) sidesteps the chicken-and-egg problem entirely.
pub(super) fn natural_chip_width(ui: &Ui, chip: &ChipViewModel, show_close: bool) -> f32 {
    let ctx = ui.ctx();
    let style = ui.style();
    let body_font = style
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(egui::FontId::default);
    let small_font = style
        .text_styles
        .get(&egui::TextStyle::Small)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(10.0));

    const LEFT_INSET: f32 = 8.0;
    const RIGHT_PADDING: f32 = 8.0;
    const DOT_DIAMETER: f32 = 8.0;
    const DOT_LABEL_SPACING: f32 = 6.0;
    // `CLOSE_INSET` + `CLOSE_SIZE` from `paint_chip`'s close-button layout.
    const CLOSE_RESERVED: f32 = 24.0;

    let primary_width = ctx.fonts_mut(|f| {
        f.layout_no_wrap(chip.primary.clone(), body_font, Color32::WHITE)
            .size()
            .x
    });
    let mut primary_line = LEFT_INSET + primary_width;
    if !matches!(chip.status, ChipStatus::Neutral) {
        primary_line += DOT_DIAMETER + DOT_LABEL_SPACING;
    }

    let mut width: f32 = primary_line;
    if let Some(secondary) = &chip.secondary {
        let secondary_width = ctx.fonts_mut(|f| {
            f.layout_no_wrap(secondary.clone(), small_font, Color32::WHITE)
                .size()
                .x
        });
        let indent = if matches!(chip.status, ChipStatus::Neutral) {
            8.0
        } else {
            22.0
        };
        width = width.max(indent + secondary_width);
    }

    width += RIGHT_PADDING;
    if show_close {
        width += CLOSE_RESERVED;
    }

    let minimum = if show_close {
        CHIP_FOCUSED_MIN_WIDTH
    } else {
        CHIP_INACTIVE_MIN_WIDTH
    };
    width.clamp(minimum, CHIP_MAX_WIDTH)
}

pub(super) struct ChipPresentation {
    pub(super) active: bool,
    pub(super) can_move_left: bool,
    pub(super) can_move_right: bool,
    pub(super) forced_width: Option<f32>,
    pub(super) row_height: f32,
    pub(super) reveal: bool,
    pub(super) quick_switch_overlay_active: bool,
}

/// Paints one chip and returns the footprint it occupies in this window, which
/// the row publishes so a sibling window's drag can resolve a drop onto this
/// chip (ADR 0033).
pub(super) fn show_chip(
    ui: &mut Ui,
    chip: &ChipViewModel,
    presentation: ChipPresentation,
    actions: &mut Vec<ChromeAction>,
) -> Rect {
    let ChipPresentation {
        active,
        can_move_left,
        can_move_right,
        forced_width,
        row_height,
        reveal,
        quick_switch_overlay_active,
    } = presentation;
    let chip_id = super::chip_widget_id(chip.id);
    let ctx = ui.ctx().clone();
    // The chip's true content-driven width, measured directly from its text
    // (see `natural_chip_width`'s doc comment for why this can't be
    // discovered by measuring the *painted* chip's response rect instead).
    let natural_size = vec2(natural_chip_width(ui, chip, active), row_height);
    // Inactive chips may be allocated below their natural size; their text
    // truncates to the width selected by the row allocator.
    let bg_size = vec2(forced_width.unwrap_or(natural_size.x), row_height);

    if ctx.is_being_dragged(chip_id) {
        // Currently being dragged: keep the payload alive, reserve the
        // chip's last-known footprint in the row (so drop targets don't
        // collapse out from under the pointer), and paint the real chip
        // floating at the pointer position, exactly as
        // `Ui::dnd_drag_source` does natively for its wrapped content.
        DragAndDrop::set_payload(&ctx, chip.id);

        let (_, ghost_rect) = ui.allocate_space(natural_size);
        ui.painter().rect_stroke(
            ghost_rect,
            4.0,
            Stroke::new(1.0, CHIP_INACTIVE_OUTLINE),
            egui::StrokeKind::Inside,
        );

        let layer_id = LayerId::new(Order::Tooltip, chip_id);
        let mut floating_ui =
            ui.new_child(UiBuilder::new().max_rect(ghost_rect).layer_id(layer_id));
        let chip_painter = floating_ui.painter().clone();
        let content_response = paint_chip(
            &chip_painter,
            &mut floating_ui,
            chip,
            ChipPaintState {
                active,
                hovered: false,
                show_close: active,
                chip_id,
                outer_rect: ghost_rect,
                quick_switch_overlay_active,
            },
            actions,
        );

        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            let delta = pointer_pos - content_response.rect.center();
            ctx.transform_layer_shapes(layer_id, TSTransform::from_translation(delta));
        }
        return ghost_rect;
    }

    let (_, bg_rect) = ui.allocate_space(bg_size);
    // Interacting the whole chip's background footprint *before* its inner
    // content is added registers it first in this frame's widget order.
    // egui resolves overlapping widgets by giving a later-registered widget
    // priority within their shared area (confirmed via
    // `egui::hit_test::thin_resize_handle_next_to_label`), so the close
    // button and rename field placed afterward, inside the same rect, still
    // reliably receive their own clicks while the rest of the chip acts as
    // a click-to-activate, press-and-hold-to-reorder surface with no
    // separate drag-handle affordance needed.
    let bg_response = ui.interact(bg_rect, chip_id, Sense::click_and_drag());
    bg_response.widget_info(|| {
        // A document chip says its state in its name, so a screen reader is
        // told what the shape is showing rather than only that a file is open
        // (ADR 0034 §8).
        let name = match chip.status.document_state() {
            Some(state) => format!("{}, {state} chip", chip.primary),
            None => format!("{} chip", chip.primary),
        };
        WidgetInfo::labeled(WidgetType::Other, true, name)
    });
    // Bring a freshly-activated chip into view (`reveal`, set by the
    // caller only on the frame its `active` id changed): with
    // `ChipLayout::SingleRowScroll`, once there's no more room to shrink
    // chips further, the row falls back to a horizontally scrolling
    // `ScrollArea` that otherwise starts - and stays - at its initial
    // offset, leaving a newly created (or newly clicked) chip past the
    // fold completely invisible until the user manually scrolled to find
    // it. A no-op outside any scrolling ancestor (`ChipLayout::Wrap`, or
    // a `SingleRowScroll` row that still fits without scrolling).
    if reveal {
        bg_response.scroll_to_me(Some(Align::Center));
    }

    // The close control is a deliberately scarce affordance
    // (`docs/gui-design.md`): only the active chip ever shows it, matching
    // the mockup where inactive chips carry a plain dark-grey outline and
    // no close affordance at all, even on hover.
    let show_close = active;
    let hovered = bg_response.hovered();
    let chip_painter = ui.painter().clone();
    let mut content_ui = ui.new_child(UiBuilder::new().max_rect(bg_rect));
    paint_chip(
        &chip_painter,
        &mut content_ui,
        chip,
        ChipPaintState {
            active,
            hovered,
            show_close,
            chip_id,
            outer_rect: bg_rect,
            quick_switch_overlay_active,
        },
        actions,
    );

    if bg_response.clicked() {
        actions.push(ChromeAction::Activate(chip.id));
    }

    // Double-clicking anywhere on the chip (including the title label,
    // which senses only hover so it doesn't compete with `bg_response` for
    // click/drag priority - see `paint_chip_primary`) starts a rename,
    // unless a rename is already in progress.
    let rename_id = rename_buffer_id(chip.id);
    let already_editing = ui.data(|d| d.get_temp::<String>(rename_id)).is_some();
    if chip.renamable && !already_editing && bg_response.double_clicked() {
        ui.data_mut(|d| d.insert_temp(rename_id, chip.primary.clone()));
        actions.push(ChromeAction::RenameStarted {
            restore_focus: None,
        });
    }

    // Use raw pointer geometry for the secondary click so the menu covers the
    // complete chip, including label/status/close child widgets, without
    // placing a final invisible response above those controls and stealing
    // their ordinary primary-click behavior. Opening this menu deliberately
    // does not activate the target chip.
    let secondary_clicked = ui.input_mut(|input| {
        let mut released = false;
        input.events.retain(|event| {
            let egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                ..
            } = event
            else {
                return true;
            };
            if !bg_rect.contains(*pos) {
                return true;
            }
            released |= !pressed;
            false
        });
        released
    });
    let menu_restore_focus_id = chip_id.with("context_menu_restore_focus");
    if secondary_clicked {
        if let Some(focused) = ui.memory(|memory| memory.focused()) {
            ui.data_mut(|data| data.insert_temp(menu_restore_focus_id, focused));
        } else {
            ui.data_mut(|data| data.remove::<Id>(menu_restore_focus_id));
        }
    }
    Popup::context_menu(&bg_response)
        .open_memory(secondary_clicked.then_some(egui::SetOpenCommand::Bool(true)))
        .show(|ui| {
            super::controls::style_context_menu(ui);
            if chip.renamable && ui.button("Rename session").clicked() {
                ui.data_mut(|data| {
                    data.insert_temp(rename_buffer_id(chip.id), chip.primary.clone())
                });
                actions.push(ChromeAction::RenameStarted {
                    restore_focus: ui.data(|data| data.get_temp(menu_restore_focus_id)),
                });
                ui.close();
            }
            if can_move_left && ui.button("Move left").clicked() {
                actions.push(ChromeAction::MoveLeft(chip.id));
                ui.close();
            }
            if can_move_right && ui.button("Move right").clicked() {
                actions.push(ChromeAction::MoveRight(chip.id));
                ui.close();
            }
            if chip.closable {
                if chip.renamable || can_move_left || can_move_right {
                    ui.separator();
                }
                let label = if chip.renamable {
                    "Close session"
                } else {
                    "Close"
                };
                if ui
                    .button(RichText::new(label).color(theme::STATUS_ERROR))
                    .clicked()
                {
                    actions.push(ChromeAction::Close(chip.id));
                    ui.close();
                }
            }
        });

    // Live reorder: while another chip is being dragged, settle the row's
    // order continuously as the pointer passes anywhere over this chip's
    // full footprint, rather than only on release. This uses raw pointer
    // geometry (not `bg_response.contains_pointer()`), because that would
    // be false wherever an inner widget (the label, status dot, or close
    // button) covers the same pixel, making the target's own visible label
    // an undroppable dead zone.
    if let Some(dragged) = DragAndDrop::payload::<ChipId>(&ctx) {
        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            if *dragged != chip.id && bg_rect.contains(pointer_pos) {
                actions.push(ChromeAction::Reorder {
                    moved: *dragged,
                    before: Some(chip.id),
                });
            }
        }
    }

    bg_rect
}

/// Paints one chip's content (status dot, label/rename field, secondary
/// text, close button) into `ui`, which the caller has already bounded to
/// the chip's footprint. Returns the response covering that content.
///
/// Renders as two lines: the primary line (status dot, stable identity, and
/// the close control, when `show_close`) and, indented beneath it, a
/// smaller/muted secondary line carrying transient terminal-provided
/// metadata (`docs/gui-design.md` "Identity precedence"). Both lines
/// truncate rather than growing the chip past `CHIP_MAX_WIDTH`.
struct ChipPaintState {
    active: bool,
    hovered: bool,
    show_close: bool,
    chip_id: Id,
    outer_rect: egui::Rect,
    quick_switch_overlay_active: bool,
}

fn paint_chip(
    painter: &egui::Painter,
    ui: &mut Ui,
    chip: &ChipViewModel,
    state: ChipPaintState,
    actions: &mut Vec<ChromeAction>,
) -> egui::Response {
    let ChipPaintState {
        active,
        hovered,
        show_close,
        chip_id,
        outer_rect,
        quick_switch_overlay_active,
    } = state;
    let corner_radius = CHIP_CORNER_RADIUS;
    // The active chip's fill is the lightest surface in the row (measured
    // against the mockup: selected-chip fill lum ~170, the panel's overall
    // brightest element), not a darker merge with the terminal content -
    // see `CHIP_ACTIVE_FILL`'s doc comment for the earlier, inverted
    // assumption this replaces. Hovering an *inactive* chip previews that
    // same lighter fill (without touching its outline) rather than
    // brightening the outline - matching `paint_new_chip_button`'s hover
    // treatment for a consistent hover language across all chip-shaped
    // controls in the row.
    let fill = if active || hovered {
        CHIP_ACTIVE_FILL
    } else {
        CHIP_INACTIVE_FILL
    };
    let stroke = if active {
        Stroke::new(1.5, CHIP_ACTIVE_OUTLINE)
    } else {
        Stroke::new(1.0, CHIP_INACTIVE_OUTLINE)
    };

    painter.rect_filled(outer_rect, corner_radius, fill);
    painter.rect_stroke(outer_rect, corner_radius, stroke, egui::StrokeKind::Inside);

    // The close control is positioned from the chip's own outer rect
    // (evenly inset from the right edge, vertically centered) rather than
    // flowing through the primary line's layout: this keeps its position
    // fixed regardless of label length and avoids the label being pulled
    // towards the right edge, which a right-to-left sub-layout previously
    // caused for short labels. Centering vertically (rather than a fixed
    // inset from the top) is what lets it track `outer_rect`'s own height
    // as chips shrink in compact mode (`CHIP_HEIGHT_COMPACT`) instead of
    // staying pinned to where the top-inset would have placed it for the
    // taller two-line chip height.
    const CLOSE_SIZE: f32 = 16.0;
    const CLOSE_INSET: f32 = 8.0;
    let close_rect = if chip.closable && show_close {
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                outer_rect.right() - CLOSE_INSET - CLOSE_SIZE,
                outer_rect.center().y - CLOSE_SIZE / 2.0,
            ),
            vec2(CLOSE_SIZE, CLOSE_SIZE),
        );
        let mut close_ui = ui.new_child(UiBuilder::new().max_rect(rect));
        paint_close_button(&mut close_ui, chip.id, actions);
        Some(rect)
    } else {
        None
    };

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().interact_size.y = 0.0;
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        let reserved = close_rect.map_or(0.0, |rect| outer_rect.right() - rect.left());
        if let Some(secondary) = &chip.secondary {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.set_min_size(outer_rect.size());
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    paint_chip_primary_contents(
                        ui,
                        chip,
                        reserved,
                        actions,
                        quick_switch_overlay_active,
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_space(if matches!(chip.status, ChipStatus::Neutral) {
                        8.0
                    } else {
                        22.0
                    });
                    ui.scope(|ui| {
                        let max_width = (ui.available_width() - reserved).max(0.0);
                        ui.set_max_width(max_width);
                        ui.add(
                            egui::Label::new(
                                RichText::new(secondary).color(CHIP_SECONDARY_TEXT).small(),
                            )
                            // Chip text is identity/navigation chrome, not
                            // selectable document content: without this, egui's
                            // default `selectable_labels` style makes the whole
                            // chip show a text (I-beam) hover cursor instead of
                            // the plain arrow a clickable chip should have.
                            .selectable(false)
                            .truncate(),
                        );
                    });
                });
                ui.add_space(4.0);
            });
        } else {
            let line_height = ui.text_style_height(&egui::TextStyle::Body);
            let line_rect = egui::Rect::from_center_size(
                outer_rect.center(),
                vec2(outer_rect.width(), line_height),
            );
            let mut line_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(line_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            paint_chip_primary_contents(
                &mut line_ui,
                chip,
                reserved,
                actions,
                quick_switch_overlay_active,
            );
        }
    });

    ui.interact(outer_rect, chip_id.with("content"), Sense::hover())
}

fn paint_chip_primary_contents(
    ui: &mut Ui,
    chip: &ChipViewModel,
    reserved: f32,
    actions: &mut Vec<ChromeAction>,
    quick_switch_overlay_active: bool,
) {
    ui.add_space(8.0);
    // Feature request #69: while the quick-switch modifier is held and the
    // preference is on, an eligible chip's quick-switch number temporarily
    // takes the place of its usual status presentation - the status dot
    // for session chips, or a reserved slot for `Neutral` chips (Launcher/
    // Settings/etc.) that otherwise paint no dot at all.
    let show_number = quick_switch_overlay_active && chip.quick_switch_number.is_some();
    if show_number {
        paint_quick_switch_number(ui, chip.quick_switch_number.expect("checked above"));
    } else if !matches!(chip.status, ChipStatus::Neutral) {
        paint_status_dot(ui, chip.status, chip.pulse_new_output);
    }

    let rename_id = rename_buffer_id(chip.id);
    let editing: Option<String> = ui.data(|d| d.get_temp(rename_id));
    ui.scope(|ui| {
        let max_width = (ui.available_width() - reserved).max(0.0);
        ui.set_max_width(max_width);
        paint_chip_primary(ui, chip, rename_id, editing, actions);
    });
}

/// Paints the primary-line label or its in-progress rename `TextEdit`,
/// filling the remaining horizontal space in `ui` (truncating rather than
/// growing the chip). Split out of [`paint_chip`] so the close button (when
/// shown) can be laid out first, right-to-left, without the label pushing
/// it out of the chip.
fn paint_chip_primary(
    ui: &mut Ui,
    chip: &ChipViewModel,
    rename_id: Id,
    editing: Option<String>,
    actions: &mut Vec<ChromeAction>,
) {
    if let Some(mut buffer) = editing {
        let response = ui.add(TextEdit::singleline(&mut buffer).desired_width(f32::INFINITY));
        let cancel = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape));
        let confirm = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
        let commit = !cancel && (confirm || response.lost_focus());
        // Re-request focus only if we're staying in edit mode; doing this
        // unconditionally would immediately re-grab focus after
        // Enter/Escape surrendered it, masking the commit/cancel.
        if !response.has_focus() && !cancel && !commit {
            response.request_focus();
        }
        if cancel {
            ui.data_mut(|d| d.remove::<String>(rename_id));
            actions.push(ChromeAction::RenameFinished);
        } else if commit {
            let trimmed = buffer.trim();
            if !trimmed.is_empty() {
                actions.push(ChromeAction::Rename {
                    id: chip.id,
                    name: trimmed.to_owned(),
                });
            }
            ui.data_mut(|d| d.remove::<String>(rename_id));
            actions.push(ChromeAction::RenameFinished);
        } else {
            ui.data_mut(|d| d.insert_temp(rename_id, buffer));
        }
    } else {
        let label = RichText::new(&chip.primary).color(CHIP_PRIMARY_TEXT);
        // Deliberately `Sense::hover()` only (no click/drag), matching the
        // secondary line below: the chip's own `bg_response` (covering the
        // whole chip footprint, including this label's pixels) is the sole
        // widget that senses click-and-drag here. Giving this label its own
        // `Sense::click()` used to let it win the *click* half of egui's
        // hit-test tie-break (it's registered on top of `bg_response`),
        // which could leave a press-and-drag started on the title text
        // attributed to whichever drag-sensing widget the hit-test fell
        // back to next - sometimes the row's own native-window-drag region
        // instead of the chip's reorder drag - rather than reliably
        // reordering the chip the same way starting a drag on the
        // secondary line already did. Activation and rename-start are
        // instead driven entirely by `bg_response` in `show_chip`.
        ui.add(
            egui::Label::new(label)
                // See the secondary-line label above: this is clickable
                // navigation chrome, not selectable text, so the hover
                // cursor should read as a plain arrow, not an I-beam.
                .selectable(false)
                .truncate(),
        );
    }
}

/// Compact, non-color-exclusive connection-state dot, painted directly
/// rather than relying on a glyph the active font may not have coverage for
/// (the previous `\u{25cf}` rendered as tofu/an empty box on this machine).
fn paint_status_dot(ui: &mut Ui, status: ChipStatus, pulse: bool) {
    let diameter = 8.0;
    // Allocate at the primary label's own line height (rather than just
    // the dot's diameter) so this row's cross-axis `Align::Center`
    // computes the same center line for both the dot and the label text,
    // instead of centering the dot within a shorter box that happens to
    // sit slightly off from the text's own optical center.
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, text_height), Sense::hover());
    let color = if pulse {
        // Feature request #68: a slow (~2.4s period), smooth fade between
        // full and low opacity - deliberately slower and gentler than the
        // fixed-solid connection-state dot so it reads as an ambient "new
        // output" cue rather than an alarm, and never changes the dot's
        // hue, which stays reserved for connection-state semantics.
        let phase = (ui.input(|i| i.time) * std::f64::consts::TAU / 2.4).sin();
        let alpha = (0.35 + 0.65 * (phase * 0.5 + 0.5)) as f32;
        ui.ctx().request_repaint();
        status.color().gamma_multiply(alpha)
    } else {
        status.color()
    };
    let radius = diameter / 2.0;
    match status.marker() {
        ChipMarker::Filled => {
            ui.painter().circle_filled(rect.center(), radius, color);
        }
        // Hollow, not merely a different hue: an edited document has to be
        // distinguishable from a saved one with the colour taken away.
        ChipMarker::Hollow => {
            ui.painter().circle_stroke(
                rect.center(),
                radius - 0.5,
                egui::Stroke::new(1.5, color),
            );
        }
        ChipMarker::Triangle => {
            let centre = rect.center();
            let points = vec![
                egui::pos2(centre.x, centre.y - radius),
                egui::pos2(centre.x + radius, centre.y + radius * 0.8),
                egui::pos2(centre.x - radius, centre.y + radius * 0.8),
            ];
            ui.painter()
                .add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
        }
    }
    response.on_hover_text(status.accessible_label());
}

/// Overlay painted in the status-dot's slot (or, for `Neutral` chips that
/// have no dot slot at all, a same-sized reserved slot) while the
/// quick-switch modifier is held and the preference is on (feature request
/// #69): the chip's 1-based `Cmd+N`/`Ctrl+N` quick-switch digit, in an
/// accent color distinct from any status color so it never reads as a new
/// connection state.
fn paint_quick_switch_number(ui: &mut Ui, number: u8) {
    let diameter = 8.0;
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    let (rect, response) = ui.allocate_exact_size(vec2(diameter, text_height), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        number.to_string(),
        egui::FontId::new(10.0, egui::FontFamily::Monospace),
        CHIP_QUICK_SWITCH_NUMBER,
    );
    let label = format!("Quick switch: {number}");
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label.clone()));
    response.on_hover_text(label);
}

/// Painter-drawn close control (replacing the previous `\u{2715}` glyph,
/// which likewise rendered as tofu): two crossed lines inside a small
/// clickable square, with an explicit accessible label so screen readers
/// and headless-test queries don't depend on the (absent) visual glyph.
fn paint_close_button(ui: &mut Ui, id: ChipId, actions: &mut Vec<ChromeAction>) {
    let size = 16.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Close"));

    let color = if response.hovered() {
        CHROME_CLOSE_HOVER
    } else {
        // Matches the active chip's own outline color (the close control
        // only ever appears on the active chip), so the two read as the
        // same "active" affordance rather than the close control looking
        // dimmer/disabled by comparison.
        CHIP_ACTIVE_OUTLINE
    };
    let inset = rect.shrink(4.0);
    ui.painter().line_segment(
        [inset.left_top(), inset.right_bottom()],
        Stroke::new(1.5, color),
    );
    ui.painter().line_segment(
        [inset.right_top(), inset.left_bottom()],
        Stroke::new(1.5, color),
    );

    let response = response.on_hover_text("Close");
    if response.clicked() {
        actions.push(ChromeAction::Close(id));
    }
}
