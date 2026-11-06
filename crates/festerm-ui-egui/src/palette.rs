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

/// Presentation categories only; command availability and authority stay with the caller.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PaletteCategory {
    Views,
    Sessions,
    Terminal,
    Panes,
    Files,
    Application,
}

impl PaletteCategory {
    pub const ALL: [Self; 6] = [
        Self::Views,
        Self::Sessions,
        Self::Terminal,
        Self::Panes,
        Self::Files,
        Self::Application,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Views => "Open views",
            Self::Sessions => "Sessions & connections",
            Self::Terminal => "Terminal",
            Self::Panes => "Panes",
            Self::Files => "Files & documents",
            Self::Application => "Application",
        }
    }
}

/// One searchable entry: an application action or an open tab.
#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub category: PaletteCategory,
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
    selected: Option<u64>,
    needs_focus: bool,
    category: Option<PaletteCategory>,
    category_widgets: Vec<Id>,
    result_widgets: Vec<(Id, u64)>,
    composition_owns_keys: bool,
}

impl PaletteState {
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Stable graphics identity shared with the optional application frame painter.
    pub fn window_id() -> Id {
        Id::new("festerm_command_palette")
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = None;
        self.category = None;
        self.category_widgets.clear();
        self.result_widgets.clear();
        self.needs_focus = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = None;
        self.category = None;
        self.category_widgets.clear();
        self.result_widgets.clear();
        self.composition_owns_keys = false;
    }

    /// The caller supplies its existing per-frame IME ownership decision.
    pub fn set_composition_owns_keys(&mut self, owns_keys: bool) {
        self.composition_owns_keys = owns_keys;
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
    let query = query.to_lowercase();
    let mut matches: Vec<_> = items
        .iter()
        .filter(|item| {
            let label = item.label.to_lowercase();
            let hint = item.hint.as_deref().unwrap_or_default().to_lowercase();
            let category = item.category.label().to_lowercase();
            query
                .split_whitespace()
                .all(|word| label.contains(word) || hint.contains(word) || category.contains(word))
        })
        .collect();
    matches.sort_by_key(|item| item.category);
    matches
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
    let focused = ctx.memory(|memory| memory.focused());
    let category_owns_keyboard = focused.is_some_and(|id| state.category_widgets.contains(&id));
    let popup_was_open = egui::Popup::is_any_open(ctx);
    let list_owns_keyboard =
        !category_owns_keyboard && !popup_was_open && !state.composition_owns_keys;
    if let Some((_, id)) = state
        .result_widgets
        .iter()
        .find(|(id, _)| Some(*id) == focused)
    {
        state.selected = Some(*id);
    }
    let navigation = list_owns_keyboard
        .then(|| {
            ctx.input_mut(|input| {
                [Key::ArrowDown, Key::ArrowUp, Key::PageDown, Key::PageUp]
                    .into_iter()
                    .find(|key| input.consume_key(Modifiers::NONE, *key))
            })
        })
        .flatten();
    let mut category_widgets = Vec::new();
    let mut result_widgets = Vec::new();
    let root = ctx.content_rect();
    let frame = egui::Frame::window(&ctx.global_style());
    let margin = frame.total_margin().sum();
    let top = if root.height() < 400.0 { 16.0 } else { 48.0 };
    let size = egui::vec2(
        (root.width() - 32.0 - margin.x).clamp(0.0, 840.0),
        (root.height() - top - 16.0 - margin.y - 40.0).clamp(0.0, 600.0),
    );

    let area_response = Window::new("Command Palette")
        .id(PaletteState::window_id())
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
                    .hint_text(state.category.map_or_else(
                        || "Search all commands and open views".to_owned(),
                        |category| format!("Search {}", category.label().to_lowercase()),
                    )),
            );
            if request_focus {
                response.request_focus();
            }
            // Preserve the query owner until IME/palette Escape policy runs.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                        ..Default::default()
                    },
                )
            });
            let mut reset_selection = request_focus || response.changed();
            let all_matches = filtered(items, &state.query);
            let previous_category = state.category;
            let wide = ui.available_width() >= 620.0;
            if !wide {
                let category = egui::ComboBox::from_id_salt("palette-category")
                    .selected_text(
                        state
                            .category
                            .map_or("All categories", PaletteCategory::label),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.category, None, "All categories");
                        for category in PaletteCategory::ALL {
                            if items.iter().any(|item| item.category == category) {
                                ui.selectable_value(
                                    &mut state.category,
                                    Some(category),
                                    category.label(),
                                );
                            }
                        }
                    });
                category_widgets.push(category.response.id);
            }
            ui.separator();
            let body_height = (ui.available_height() - 30.0).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), body_height),
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    if wide {
                        ui.allocate_ui_with_layout(
                            egui::vec2(192.0, body_height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(192.0);
                                ScrollArea::vertical()
                                    .id_salt("palette-categories")
                                    .auto_shrink([false, false])
                                    .max_height(body_height)
                                    .show(ui, |ui| {
                                        let button = ui.add_sized(
                                            [ui.available_width(), 30.0],
                                            egui::Button::selectable(
                                                state.category.is_none(),
                                                (
                                                    "All categories",
                                                    egui::Atom::grow(),
                                                    egui::RichText::new(
                                                        all_matches.len().to_string(),
                                                    )
                                                    .weak(),
                                                ),
                                            )
                                            .frame_when_inactive(false)
                                            .truncate(),
                                        );
                                        category_widgets.push(button.id);
                                        if button.clicked() {
                                            state.category = None;
                                        }
                                        for category in PaletteCategory::ALL {
                                            if !items.iter().any(|item| item.category == category)
                                                && state.category != Some(category)
                                            {
                                                continue;
                                            }
                                            let count = all_matches
                                                .iter()
                                                .filter(|item| item.category == category)
                                                .count();
                                            let button = ui.add_sized(
                                                [ui.available_width(), 30.0],
                                                egui::Button::selectable(
                                                    state.category == Some(category),
                                                    (
                                                        category.label(),
                                                        egui::Atom::grow(),
                                                        egui::RichText::new(count.to_string())
                                                            .weak(),
                                                    ),
                                                )
                                                .frame_when_inactive(false)
                                                .truncate(),
                                            );
                                            category_widgets.push(button.id);
                                            if button.clicked() {
                                                state.category = Some(category);
                                            }
                                        }
                                    });
                            },
                        );
                        ui.separator();
                    }
                    reset_selection |= previous_category != state.category;
                    let matches: Vec<_> = all_matches
                        .iter()
                        .copied()
                        .filter(|item| {
                            state
                                .category
                                .is_none_or(|category| item.category == category)
                        })
                        .collect();
                    if reset_selection {
                        state.selected = matches.first().map(|item| item.id);
                    } else if !matches.iter().any(|item| Some(item.id) == state.selected) {
                        state.selected = None;
                    }
                    let mut scroll_selection = reset_selection;
                    if let Some(key) = navigation {
                        let current = matches
                            .iter()
                            .position(|item| Some(item.id) == state.selected);
                        let page = (body_height / 30.0).floor().max(1.0) as usize;
                        let index = match key {
                            Key::ArrowDown => current.map_or(0, |index| index.saturating_add(1)),
                            Key::ArrowUp => current.unwrap_or(0).saturating_sub(1),
                            Key::PageDown => current.map_or(0, |index| index.saturating_add(page)),
                            Key::PageUp => current.unwrap_or(0).saturating_sub(page),
                            _ => unreachable!("only list navigation keys are captured"),
                        };
                        state.selected = matches
                            .get(index.min(matches.len().saturating_sub(1)))
                            .map(|item| item.id);
                        scroll_selection = true;
                        response.request_focus();
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), body_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ScrollArea::vertical()
                                .id_salt(("palette-results", state.category))
                                .auto_shrink([false, false])
                                .max_height(body_height)
                                .show(ui, |ui| {
                                    if matches.is_empty() {
                                        ui.label("No matching commands or open views.");
                                    }
                                    // `with_cross_justify(true)` makes each row fill the full
                                    // available width instead of shrinking to its own text, so
                                    // the highlighted background spans the whole popup rather
                                    // than just the label.
                                    ui.with_layout(
                                        egui::Layout::top_down(egui::Align::Min)
                                            .with_cross_justify(true),
                                        |ui| {
                                            let mut previous = None;
                                            for item in &matches {
                                                if previous != Some(item.category) {
                                                    if previous.is_some() {
                                                        ui.add_space(10.0);
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(item.category.label())
                                                            .small()
                                                            .strong()
                                                            .color(ui.visuals().weak_text_color()),
                                                    );
                                                    previous = Some(item.category);
                                                }
                                                let highlighted = Some(item.id) == state.selected;
                                                ui.push_id(item.id, |ui| {
                                                    let row_shortcut = if item.is_tab {
                                                        item.shortcut_label.as_deref()
                                                    } else {
                                                        item.hint.as_deref()
                                                    };
                                                    let left_text = if item.is_tab {
                                                        match &item.hint {
                                                            Some(hint) => format!(
                                                                "{}  \u{2014}  {hint}",
                                                                item.label
                                                            ),
                                                            None => item.label.clone(),
                                                        }
                                                    } else {
                                                        item.label.clone()
                                                    };
                                                    let accessible_label = match row_shortcut {
                                                        Some(shortcut) => {
                                                            format!("{left_text}, {shortcut}")
                                                        }
                                                        None => left_text.clone(),
                                                    };
                                                    let row_height =
                                                        ui.spacing().interact_size.y.max(28.0);
                                                    let (rect, response) = ui.allocate_exact_size(
                                                        egui::vec2(
                                                            ui.available_width(),
                                                            row_height,
                                                        ),
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
                                                        ui.painter().rect_filled(
                                                            rect,
                                                            3.0,
                                                            selection.bg_fill,
                                                        );
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
                                                            ui.visuals()
                                                                .widgets
                                                                .hovered
                                                                .weak_bg_fill,
                                                        );
                                                    }

                                                    let font =
                                                        TextStyle::Button.resolve(ui.style());
                                                    let left_color = if highlighted {
                                                        ui.visuals().strong_text_color()
                                                    } else {
                                                        ui.visuals().text_color()
                                                    };
                                                    let painter = ui.painter().with_clip_rect(
                                                        rect.intersect(ui.clip_rect()),
                                                    );
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
                                                            natural
                                                                .size()
                                                                .x
                                                                .min(rect.width() * 0.35),
                                                        )
                                                    });
                                                    let shortcut_width =
                                                        shortcut.as_ref().map_or(0.0, |galley| {
                                                            galley.size().x + 12.0
                                                        });
                                                    let left = elided_row_text(
                                                        &painter,
                                                        &left_text,
                                                        font,
                                                        left_color,
                                                        (rect.width() - 14.0 - shortcut_width)
                                                            .max(0.0),
                                                    );
                                                    painter.galley(
                                                        rect.left_center()
                                                            + egui::vec2(6.0, -left.size().y / 2.0),
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
                                                        state.selected = Some(item.id);
                                                        decision = Some(Some(item.id));
                                                    }
                                                    result_widgets.push((response.id, item.id));
                                                    if highlighted && scroll_selection {
                                                        response.scroll_to_me(Some(
                                                            egui::Align::Center,
                                                        ));
                                                    }
                                                });
                                            }
                                        },
                                    );
                                });
                        },
                    );
                },
            );
            if previous_category != state.category {
                response.request_focus();
            }
            let count = all_matches
                .iter()
                .filter(|item| {
                    state
                        .category
                        .is_none_or(|category| item.category == category)
                })
                .count();
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{count} {}",
                        if count == 1 { "result" } else { "results" }
                    ))
                    .small()
                    .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("Up/Down · Page Up/Down · Enter · Esc")
                            .small()
                            .weak(),
                    );
                });
            });
            let enter_pressed = list_owns_keyboard
                && previous_category == state.category
                && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
            if enter_pressed && decision.is_none() {
                decision = state.selected.map(Some);
                response.request_focus();
            }
            let escape_pressed = !popup_was_open
                && !state.composition_owns_keys
                && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
            if escape_pressed {
                decision = Some(None);
            }
        });
    state.category_widgets = category_widgets;
    state.result_widgets = result_widgets;

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
    if decision.is_none() && !popup_was_open {
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
    use egui_kittest::kittest::NodeT as _;

    fn item(id: u64, label: &str) -> PaletteItem {
        PaletteItem {
            category: PaletteCategory::Application,
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
                category: PaletteCategory::Views,
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
        assert!(state.query.is_empty());
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
        harness_at(egui::vec2(500.0, 400.0), state)
    }

    fn harness_at(
        size: egui::Vec2,
        state: PaletteHarnessState,
    ) -> egui_kittest::Harness<'static, PaletteHarnessState> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                |ui, state: &mut PaletteHarnessState| {
                    if let Some(decision) = show(ui.ctx(), &mut state.palette, &state.items) {
                        state.decision = Some(decision);
                    }
                },
                state,
            )
    }

    fn categorized_items() -> Vec<PaletteItem> {
        vec![
            PaletteItem {
                category: PaletteCategory::Sessions,
                ..item(1, "New Session")
            },
            PaletteItem {
                category: PaletteCategory::Terminal,
                ..item(2, "Copy")
            },
            PaletteItem {
                category: PaletteCategory::Views,
                hint: Some("release notes".into()),
                ..item(3, "production-db")
            },
        ]
    }

    #[test]
    fn grouped_search_matches_each_word_across_identity_hint_and_category() {
        let items = categorized_items();
        assert_eq!(
            filtered(&items, "")
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            [3, 1, 2]
        );
        assert_eq!(filtered(&items, "OPEN production RELEASE")[0].id, 3);
        assert_eq!(filtered(&items, "connections new")[0].id, 1);
        assert!(filtered(&items, "production terminal").is_empty());
    }

    #[test]
    fn category_scope_and_responsive_navigation_preserve_command_ids() {
        use egui_kittest::kittest::Queryable as _;
        for size in [egui::vec2(1000.0, 760.0), egui::vec2(360.0, 516.0)] {
            let mut palette = PaletteState::default();
            palette.open();
            let mut harness = harness_at(
                size,
                PaletteHarnessState {
                    palette,
                    items: categorized_items(),
                    decision: None,
                },
            );
            harness.run();
            assert_eq!(
                harness.state().palette.category_widgets.len(),
                if size.x > 620.0 { 4 } else { 1 }
            );
            // Exercise the real responsive category control, including its compact popup.
            if size.x < 620.0 {
                harness.get_by_role(egui::accesskit::Role::ComboBox).click();
                harness.run();
            }
            harness
                .query_all_by_label_contains("Sessions & connections")
                .into_iter()
                .find(|node| {
                    node.accesskit_node().role() != egui::accesskit::Role::Label
                        && node.accesskit_node().role() != egui::accesskit::Role::TextRun
                })
                .expect("interactive category control")
                .click();
            harness.run();
            assert_eq!(
                harness.state().palette.category,
                Some(PaletteCategory::Sessions)
            );
            assert!(harness.query_by_label("Copy").is_none());
            assert_eq!(harness.state().decision, None);
            harness.key_press(Key::Enter);
            harness.run();
            assert_eq!(harness.state().decision, Some(Some(1)));
        }
    }

    #[test]
    fn category_keyboard_enter_never_dispatches_a_result() {
        for size in [egui::vec2(1000.0, 760.0), egui::vec2(360.0, 516.0)] {
            let mut palette = PaletteState::default();
            palette.open();
            let mut harness = harness_at(
                size,
                PaletteHarnessState {
                    palette,
                    items: categorized_items(),
                    decision: None,
                },
            );
            harness.run();
            let category = harness.state().palette.category_widgets[0];
            harness
                .ctx
                .memory_mut(|memory| memory.request_focus(category));
            harness.key_press(Key::Enter);
            harness.run();
            assert_eq!(harness.state().decision, None);
        }
    }

    #[test]
    fn composing_query_owns_navigation_enter_and_escape_without_refocusing() {
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "Alpha"), item(2, "Alpine")],
            decision: None,
        });
        harness.run();
        harness.state_mut().palette.set_composition_owns_keys(true);
        harness.event(egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "Al".into(),
            active_range_chars: None,
        }));
        harness.run();
        harness.key_press(Key::ArrowDown);
        harness.run();
        assert_eq!(harness.state().palette.selected, Some(1));
        assert!(!harness
            .output()
            .platform_output
            .ime
            .as_ref()
            .is_some_and(|ime| ime.should_interrupt_composition));
        for key in [Key::Enter, Key::Escape] {
            harness.key_press(key);
            harness.run();
            assert_eq!(harness.state().decision, None);
        }
    }

    #[test]
    fn category_popup_escape_closes_popup_before_palette() {
        use egui_kittest::kittest::Queryable as _;
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: categorized_items(),
            decision: None,
        });
        harness.run();
        harness.get_by_role(egui::accesskit::Role::ComboBox).click();
        harness.run();
        assert!(egui::Popup::is_any_open(&harness.ctx));
        harness.key_press(Key::Escape);
        harness.run();
        assert!(!egui::Popup::is_any_open(&harness.ctx));
        assert_eq!(harness.state().decision, None);
        harness.key_press(Key::Escape);
        harness.run();
        assert_eq!(harness.state().decision, Some(None));
    }

    #[test]
    fn full_category_sidebar_fits_a_short_wide_root() {
        let size = egui::vec2(1000.0, 240.0);
        let mut palette = PaletteState::default();
        palette.open();
        let items = PaletteCategory::ALL
            .into_iter()
            .enumerate()
            .map(|(id, category)| PaletteItem {
                category,
                ..item(id as u64, category.label())
            })
            .collect();
        let mut harness = harness_at(
            size,
            PaletteHarnessState {
                palette,
                items,
                decision: None,
            },
        );
        harness.run();
        let window = harness
            .ctx
            .memory(|memory| memory.area_rect(PaletteState::window_id()))
            .unwrap();
        assert!(
            egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(window),
            "{window:?}"
        );
        assert_eq!(harness.state().palette.category_widgets.len(), 7);
    }

    #[test]
    fn category_popup_click_outside_palette_keeps_palette_open_until_selection() {
        use egui_kittest::kittest::Queryable as _;
        let mut palette = PaletteState::default();
        palette.open();
        let items = PaletteCategory::ALL
            .into_iter()
            .enumerate()
            .map(|(id, category)| PaletteItem {
                category,
                ..item(id as u64, &format!("Action {id}"))
            })
            .collect();
        let mut harness = harness_at(
            egui::vec2(360.0, 240.0),
            PaletteHarnessState {
                palette,
                items,
                decision: None,
            },
        );
        harness.run();
        harness.get_by_role(egui::accesskit::Role::ComboBox).click();
        harness.run();
        let window = harness
            .ctx
            .memory(|memory| memory.area_rect(PaletteState::window_id()))
            .unwrap();
        let target = harness
            .query_all_by_label_contains("Application")
            .find(|node| node.accesskit_node().role() == egui::accesskit::Role::Button)
            .expect("popup category");
        let position = target.rect().center();
        assert!(
            !window.contains(position),
            "must exercise a category outside the palette: {window:?} {position:?}"
        );
        harness.input_mut().events.extend([
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        harness.step();
        assert_eq!(harness.state().decision, None);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run();
        assert_eq!(harness.state().decision, None);
        assert_eq!(
            harness.state().palette.category,
            Some(PaletteCategory::Application)
        );
    }

    #[test]
    fn dynamic_results_keep_selected_identity_and_never_retarget_removed_selection() {
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "First"), item(2, "Second"), item(3, "Third")],
            decision: None,
        });
        harness.run();
        harness.key_press(Key::ArrowDown);
        harness.run();
        assert_eq!(harness.state().palette.selected, Some(2));
        harness.state_mut().items.swap(0, 2);
        harness.run();
        assert_eq!(harness.state().palette.selected, Some(2));
        harness.state_mut().items.retain(|item| item.id != 2);
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().palette.selected, None);
        assert_eq!(harness.state().decision, None);
        harness.key_press(Key::ArrowDown);
        harness.run();
        assert_eq!(harness.state().palette.selected, Some(3));
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().decision, Some(Some(3)));
    }

    #[test]
    fn paged_keyboard_navigation_scrolls_selected_result_into_view() {
        use egui_kittest::kittest::Queryable as _;
        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness_at(
            egui::vec2(1000.0, 760.0),
            PaletteHarnessState {
                palette,
                items: (0..80)
                    .map(|id| item(id, &format!("Command {id}")))
                    .collect(),
                decision: None,
            },
        );
        harness.run();
        for _ in 0..3 {
            harness.key_press(Key::PageDown);
            harness.run();
        }
        let selected = harness.state().palette.selected.unwrap();
        assert!(selected > 20);
        let window = harness
            .ctx
            .memory(|memory| memory.area_rect(PaletteState::window_id()))
            .unwrap();
        assert!(window.contains_rect(harness.get_by_label(&format!("Command {selected}")).rect()));
        harness.key_press(Key::Enter);
        harness.run();
        assert_eq!(harness.state().decision, Some(Some(selected)));
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
                category: PaletteCategory::Views,
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
                            category: PaletteCategory::Views,
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
