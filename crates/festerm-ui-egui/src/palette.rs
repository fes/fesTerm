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

use egui::{Context, Id, Key, Modifiers, RichText, ScrollArea, Window};

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

    Window::new("Command Palette")
        .id(Id::new("festerm_command_palette"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 48.0))
        .fixed_size(egui::vec2(420.0, 320.0))
        .show(ctx, |ui| {
            let response = ui.text_edit_singleline(&mut state.query);
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
            ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (index, item) in matches.iter().enumerate() {
                    let highlighted = index == state.selected;
                    let text = if let Some(hint) = &item.hint {
                        RichText::new(format!("{}  \u{2014}  {}", item.label, hint))
                    } else {
                        RichText::new(&item.label)
                    };
                    let text = if highlighted { text.strong() } else { text };
                    if ui.selectable_label(highlighted, text).clicked() {
                        decision = Some(Some(item.id));
                    }
                }
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

    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, label: &str) -> PaletteItem {
        PaletteItem {
            id,
            label: label.to_owned(),
            hint: None,
        }
    }

    #[test]
    fn empty_query_matches_every_item() {
        let items = vec![item(1, "New Launcher Tab"), item(2, "Open Settings")];
        assert_eq!(filtered(&items, "").len(), 2);
    }

    #[test]
    fn query_matches_case_insensitively_on_label_or_hint() {
        let items = vec![
            PaletteItem {
                id: 1,
                label: "production-db".to_owned(),
                hint: Some("nvim server.rs".to_owned()),
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
            items: vec![item(1, "New Launcher Tab"), item(2, "Open Settings")],
            decision: None,
        });
        harness.run();

        harness.get_by_label("Open Settings").click();
        harness.run();

        assert_eq!(harness.state().decision, Some(Some(2)));
    }

    #[test]
    fn typing_filters_and_enter_selects_the_highlighted_row() {
        use egui_kittest::kittest::Queryable as _;

        let mut palette = PaletteState::default();
        palette.open();
        let mut harness = harness(PaletteHarnessState {
            palette,
            items: vec![item(1, "New Launcher Tab"), item(2, "Open Settings")],
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
            items: vec![item(1, "New Launcher Tab")],
            decision: None,
        });
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.run();

        assert_eq!(harness.state().decision, Some(None));
    }
}
