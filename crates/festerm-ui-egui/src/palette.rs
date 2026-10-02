//! Command palette / searchable session switcher overlay.
//!
//! `docs/gui-design.md` ("Interaction Conventions" and "Tab overflow and
//! wrapping") calls for both "Command palette or equivalent access" for less
//! common actions and "a searchable session switcher keyed primarily by
//! stable identity". This module implements one overlay that serves both
//! roles: callers supply a flat list of [`PaletteItem`]s (actions and/or
//! open tabs) and this widget only filters, navigates, and reports which
//! item the user picked.
//!
//! This module is pure presentation, matching `chrome.rs`: it owns no
//! application/tab/session policy and performs no dispatch. Callers (the
//! application layer) translate a selected [`PaletteItem::id`] into an
//! `AppCommand` through the single command-handling path
//! (`docs/application-command-model.md`).

use egui::{
    Context, Id, Key, Modifiers, ScrollArea, Sense, TextStyle, WidgetInfo, WidgetType, Window,
};

/// One searchable entry: an application action or an open tab.
#[derive(Clone, Debug)]
pub struct PaletteItem {
    /// Opaque id the caller correlates back to a concrete command or tab.
    /// Carries no terminal content.
    pub id: u64,
    /// Stable primary label, e.g. an action name or a tab's stable identity
    /// (`docs/gui-design.md` "Identity precedence").
    pub label: String,
    /// Optional secondary text such as terminal-provided dynamic title or a
    /// short hint. Never the sole identifying text.
    pub hint: Option<String>,
    /// Whether this row activates an open tab rather than running a
    /// one-shot action.
    pub is_tab: bool,
    /// Pre-formatted quick-switch keystroke shown in the muted right column
    /// (e.g. `"\u{2318}1"`), for the first several open tabs. The same keystroke
    /// also switches directly to that tab outside the palette
    /// (`app::ApplicationShortcut`-style global handling); this field only
    /// controls how it is displayed here. Callers format the platform
    /// modifier glyph/text themselves, matching how `hint` is already
    /// pre-formatted (e.g. `"Cmd+T"`), so this module stays pure
    /// presentation.
    pub shortcut_label: Option<String>,
}

/// Persistent (frame-to-frame) palette state: open/closed, current query
/// text, and which filtered row is highlighted.
#[derive(Default)]
pub struct PaletteState {
    open: bool,
    query: String,
    selected: usize,
    needs_focus: bool,
}

impl PaletteState {
    pub const fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.needs_focus = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }
}

fn filtered<'a>(items: &'a [PaletteItem], query: &str) -> Vec<&'a PaletteItem> {
    if query.is_empty() {
        return items.iter().collect();
    }
    let query = query.to_lowercase();
    items
        .iter()
        .filter(|item| {
            item.label.to_lowercase().contains(&query)
                || item
                    .hint
                    .as_ref()
                    .is_some_and(|hint| hint.to_lowercase().contains(&query))
        })
        .collect()
}

/// Renders the palette overlay when open. Returns:
/// - `Some(Some(id))` when the user picked an item this frame;
/// - `Some(None)` when the user dismissed the palette without picking one;
/// - `None` when the palette stayed open with no decision yet, or was
///   already closed.
///
/// The caller is responsible for closing `state` and dispatching the
/// resulting id; this widget only reports the gesture.
pub fn show(ctx: &Context, state: &mut PaletteState, items: &[PaletteItem]) -> Option<Option<u64>> {
    if !state.open {
        return None;
    }

    let mut decision = None;
    let request_focus = state.needs_focus;
    state.needs_focus = false;
    let root = ctx.content_rect();
    let frame = egui::Frame::window(&ctx.global_style());
    let margin = frame.total_margin().sum();
    let top = if root.height() < 400.0 { 16.0 } else { 48.0 };
    let size = egui::vec2(
        (root.width() - 32.0 - margin.x).clamp(0.0, 420.0),
        (root.height() - top - 16.0 - margin.y - 40.0).clamp(0.0, 320.0),
    );

    let area_response = Window::new("Command Palette")
        .id(Id::new("festerm_command_palette"))
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .min_size(egui::Vec2::ZERO)
        .fixed_size(size)
        .show(ctx, |ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search sessions and commands"),
            );
            if request_focus {
                response.request_focus();
            }
            if response.changed() {
                state.selected = 0;
            }

            let matches = filtered(items, &state.query);
            if matches.is_empty() {
                state.selected = 0;
            } else if state.selected >= matches.len() {
                state.selected = matches.len() - 1;
            }

            // Keyboard navigation: Up/Down move the highlighted row, Enter
            // confirms it, Escape dismisses without a selection.
            let down_pressed =
                ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowDown));
            let up_pressed = ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowUp));
            let enter_pressed =
                ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
            let escape_pressed =
                ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));

            if down_pressed && !matches.is_empty() {
                state.selected = (state.selected + 1).min(matches.len() - 1);
            }
            if up_pressed && state.selected > 0 {
                state.selected -= 1;
            }

            ui.separator();
            ScrollArea::vertical()
                .max_height(ui.available_height().clamp(0.0, 220.0))
                .show(ui, |ui| {
                    // `with_cross_justify(true)` makes each row fill the full
                    // available width instead of shrinking to its own text, so
                    // the highlighted background spans the whole popup rather
                    // than just the label.
                    ui.with_layout(
                        egui::Layout::top_down(egui::Align::Min).with_cross_justify(true),
                        |ui| {
                            for (index, item) in matches.iter().enumerate() {
                                let highlighted = index == state.selected;
                                let row_shortcut = if item.is_tab {
                                    item.shortcut_label.as_deref()
                                } else {
                                    item.hint.as_deref()
                                };
                                let left_text = if item.is_tab {
                                    match &item.hint {
                                        Some(hint) => format!("{}  \u{2014}  {hint}", item.label),
                                        None => item.label.clone(),
                                    }
                                } else {
                                    item.label.clone()
                                };
                                let accessible_label = match row_shortcut {
                                    Some(shortcut) => format!("{left_text}, {shortcut}"),
                                    None => left_text.clone(),
                                };
                                let row_height = ui.spacing().interact_size.y.max(28.0);
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_height),
                                    Sense::click(),
                                );
                                response.widget_info(|| {
                                    let mut info = WidgetInfo::labeled(
                                        WidgetType::SelectableLabel,
                                        true,
                                        accessible_label.clone(),
                                    );
                                    info.selected = Some(highlighted);
                                    info
                                });
                                let selection = ui.visuals().selection;
                                if highlighted {
                                    ui.painter().rect_filled(rect, 3.0, selection.bg_fill);
                                    ui.painter().rect_stroke(
                                        rect,
                                        3.0,
                                        selection.stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                } else if response.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        3.0,
                                        ui.visuals().widgets.hovered.weak_bg_fill,
                                    );
                                }

                                let font = TextStyle::Button.resolve(ui.style());
                                let left_color = if highlighted {
                                    ui.visuals().strong_text_color()
                                } else {
                                    ui.visuals().text_color()
                                };
                                let painter =
                                    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                                let shortcut = row_shortcut.map(|shortcut| {
                                    let natural = painter.layout_no_wrap(
                                        shortcut.to_owned(),
                                        font.clone(),
                                        ui.visuals().weak_text_color(),
                                    );
                                    elided_row_text(
                                        &painter,
                                        shortcut,
                                        font.clone(),
                                        ui.visuals().weak_text_color(),
                                        natural.size().x.min(rect.width() * 0.35),
                                    )
                                });
                                let shortcut_width = shortcut
                                    .as_ref()
                                    .map_or(0.0, |galley| galley.size().x + 12.0);
                                let left = elided_row_text(
                                    &painter,
                                    &left_text,
                                    font,
                                    left_color,
                                    (rect.width() - 14.0 - shortcut_width).max(0.0),
                                );
                                painter.galley(
                                    rect.left_center() + egui::vec2(6.0, -left.size().y / 2.0),
                                    left,
                                    left_color,
                                );
                                if let Some(shortcut) = shortcut {
                                    painter.galley(
                                        rect.right_center()
                                            - egui::vec2(
                                                8.0 + shortcut.size().x,
                                                shortcut.size().y / 2.0,
                                            ),
                                        shortcut,
                                        ui.visuals().weak_text_color(),
                                    );
                                }
                                if response.clicked() {
                                    decision = Some(Some(item.id));
                                }
                            }
                        },
                    );
                });

            if enter_pressed {
                if let Some(item) = matches.get(state.selected) {
                    decision = Some(Some(item.id));
                }
            }
            if escape_pressed {
                decision = Some(None);
            }
        });

    // Clicking anywhere outside the popup dismisses it without a selection,
    // matching how the launcher's own overlays and native command palettes
    // behave (`docs/gui-design.md` "Quiet by default"). We check for a
    // fresh primary-button *press* outside the window rect rather than
    // using `Response::clicked_elsewhere()`: that API only resolves once
    // the corresponding release is fully attributed to a layer, which can
    // lag by a frame or more and would otherwise dismiss the palette on a
    // later frame than the click that opened it (the same click a caller
    // used to toggle the palette open, e.g. a chrome button, must never be
    // mistaken for a dismiss).
    if decision.is_none() {
        if let Some(area_response) = area_response {
            let window_rect = area_response.response.rect;
            let pressed_outside = ctx.input(|input| {
                input.pointer.primary_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|pos| !window_rect.contains(pos))
            });
            if pressed_outside {
                decision = Some(None);
            }
        }
    }

    decision
}

fn elided_row_text(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, label: &str) -> PaletteItem {
        PaletteItem {
            id,
            label: label.to_owned(),
            hint: None,
            is_tab: false,
            shortcut_label: None,
        }
    }

    #[test]
    fn empty_query_matches_every_item() {
        let items = vec![item(1, "New Session…"), item(2, "Open Settings")];
        assert_eq!(filtered(&items, "").len(), 2);
    }

    #[test]
    fn query_matches_case_insensitively_on_label_or_hint() {
        let items = vec![
            PaletteItem {
                id: 1,
                label: "production-db".to_owned(),
                hint: Some("nvim server.rs".to_owned()),
                is_tab: false,
                shortcut_label: None,
            },
            item(2, "Open Settings"),
        ];
        assert_eq!(filtered(&items, "PRODUCTION").len(), 1);
        assert_eq!(filtered(&items, "nvim").len(), 1);
        assert_eq!(filtered(&items, "settings").len(), 1);
        assert!(filtered(&items, "nonexistent").is_empty());
    }

    #[test]
    fn toggle_flips_open_state_and_resets_query_on_open() {
        let mut state = PaletteState::default();
        assert!(!state.is_open());
        state.toggle();
        assert!(state.is_open());
        state.query = "abc".to_owned();
        state.toggle();
        assert!(!state.is_open());
        state.toggle();
        assert!(state.is_open());
        assert_eq!(state.query, "", "opening resets the search query");
    }

    struct PaletteHarnessState {
        palette: PaletteState,
        items: Vec<PaletteItem>,
        decision: Option<Option<u64>>,
    }

    fn harness(state: PaletteHarnessState) -> egui_kittest::Harness<'static, PaletteHarnessState> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(500.0, 400.0))
            .build_ui_state(
                |ui, state: &mut PaletteHarnessState| {
                    if let Some(decision) = show(ui.ctx(), &mut state.palette, &state.items) {
                        state.decision = Some(decision);
                    }
                },
                state,
            )
    }

    #[test]
    fn clicking_a_filtered_result_reports_its_id() {
        use egui_kittest::kittest::Queryable as _;

        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "New Session…"), item(2, "Open Settings")],
            decision: None,
        });
        harness.run();

        harness.get_by_label("Open Settings").click();
        harness.run();

        assert_eq!(harness.state().decision, Some(Some(2)));
    }

    #[test]
    fn a_tab_row_highlights_across_its_whole_width_not_just_its_label() {
        use egui_kittest::kittest::Queryable as _;

        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![PaletteItem {
                id: 1,
                label: "x".to_owned(),
                hint: None,
                is_tab: true,
                shortcut_label: None,
            }],
            decision: None,
        });
        harness.run();

        // The row's selectable widget must claim close to the popup's full
        // interior width (not just its one-character label), so hovering
        // or clicking anywhere across the row — like every non-tab palette
        // action — highlights and activates it.
        let row = harness.get_by_label("x");
        assert!(
            row.rect().width() > 300.0,
            "tab row must fill the row width, got {}",
            row.rect().width()
        );
    }

    #[test]
    fn typing_filters_and_enter_selects_the_highlighted_row() {
        use egui_kittest::kittest::Queryable as _;

        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "New Session…"), item(2, "Open Settings")],
            decision: None,
        });
        harness.run();

        let text_input = harness.get_by_role(egui::accesskit::Role::TextInput);
        text_input.focus();
        text_input.type_text("settings");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert_eq!(harness.state().decision, Some(Some(2)));
    }

    #[test]
    fn escape_dismisses_without_a_selection() {
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "New Session…")],
            decision: None,
        });
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.run();

        assert_eq!(harness.state().decision, Some(None));
    }

    #[test]
    fn palette_fits_the_full_root_and_elides_long_rows_without_losing_identity() {
        use egui_kittest::kittest::Queryable as _;

        for size in [
            egui::vec2(752.0, 516.0),
            egui::vec2(360.0, 516.0),
            egui::vec2(360.0, 240.0),
        ] {
            let label = "staging-release-with-a-very-long-stable-session-identity";
            let hint = r"C:\synthetic-fixtures\project-with-a-long-name\review\NOTES.md";
            let mut palette = PaletteState::default();
            palette.open();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_ui_state(
                    |ui, state: &mut PaletteHarnessState| {
                        if let Some(decision) = show(ui.ctx(), &mut state.palette, &state.items) {
                            state.decision = Some(decision);
                        }
                    },
                    PaletteHarnessState {
                        palette,
                        items: vec![PaletteItem {
                            id: 42,
                            label: label.to_owned(),
                            hint: Some(hint.to_owned()),
                            is_tab: true,
                            shortcut_label: Some("Ctrl+Shift+9".to_owned()),
                        }],
                        decision: None,
                    },
                );
            harness.ctx.set_theme(egui::ThemePreference::Dark);
            harness.ctx.set_visuals(crate::theme::default_visuals());
            harness
                .ctx
                .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
            harness.run();

            let root = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let window = harness
                .ctx
                .memory(|memory| memory.area_rect(Id::new("festerm_command_palette")))
                .unwrap();
            assert!(root.contains_rect(window), "{size:?}: {window:?}");
            let identity = format!("{label}  \u{2014}  {hint}, Ctrl+Shift+9");
            let row = harness.get_by_label(&identity);
            assert!(root.contains_rect(row.rect()));
            assert!(row.rect().height() >= 24.0);
            let text_rects: Vec<_> = harness
                .output()
                .shapes
                .iter()
                .filter_map(|shape| {
                    let egui::Shape::Text(text) = &shape.shape else {
                        return None;
                    };
                    (text.galley.job.text.contains(label) || text.galley.job.text == "Ctrl+Shift+9")
                        .then(|| text.galley.rect.translate(text.pos.to_vec2()))
                })
                .collect();
            assert_eq!(text_rects.len(), 2);
            assert!(text_rects
                .iter()
                .all(|rect| row.rect().contains_rect(*rect)));
            assert!(
                text_rects[0].right() <= text_rects[1].left(),
                "label and shortcut must not overlap"
            );
            assert!(harness
                .get_by_role(egui::accesskit::Role::TextInput)
                .is_focused());
            row.click();
            harness.run();
            assert_eq!(harness.state().decision, Some(Some(42)));
        }
    }

    #[test]
    fn clicking_outside_the_popup_dismisses_without_a_selection() {
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "New Session…")],
            decision: None,
        });
        harness.run();

        // The 500x400 harness canvas anchors the 420x320 popup centered near
        // the top, so its bottom-left corner is reliably outside it.
        let outside = egui::pos2(5.0, 395.0);
        harness.event(egui::Event::PointerMoved(outside));
        harness.event(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        assert_eq!(harness.state().decision, Some(None));
    }

    #[test]
    fn the_click_that_opens_the_palette_does_not_also_dismiss_it() {
        // Regression test: a real click's press and release land on
        // separate frames (the press before a caller's button reacts on
        // release). If a caller opens the palette in reaction to that
        // release, the palette must not immediately treat the very same
        // click as an "outside" dismissal just because its press landed
        // outside where the window is about to be drawn for the first
        // time.
        let mut harness = harness(PaletteHarnessState {
            palette: PaletteState::default(),
            items: vec![item(1, "New Session…")],
            decision: None,
        });
        harness.run();

        // The 500x400 harness canvas anchors the 420x320 popup centered near
        // the top, so its bottom-left corner is reliably outside it.
        let outside = egui::pos2(5.0, 395.0);

        // Frame N-1: the press that will eventually resolve into the click
        // that opens the palette (e.g. a button elsewhere in the chrome).
        // The palette is still closed and drawn nowhere this frame.
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(outside));
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        // Frame N: the release resolves the click; the caller's button
        // handling reacts to it by opening the palette this same frame.
        harness.state_mut().palette.open();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        assert_eq!(
            harness.state().decision,
            None,
            "the palette must stay open on the frame it was opened"
        );
    }
}
