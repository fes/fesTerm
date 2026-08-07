//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::{self, Ui};
use festerm_ui_egui::chrome::ChipLayout;

use crate::tabs::{AppCommand, TabId};

/// One selectable launch option in the launcher list, alongside the
/// `AppCommand` it dispatches when chosen (by click or via keyboard).
struct LauncherItem {
    label: &'static str,
    command: AppCommand,
}

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. SSH and saved local
/// profiles remain later milestones (M7/M8); only the local-shell path is
/// wired today, and unavailable categories are omitted rather than shown as
/// disabled clutter beyond this one illustrative placeholder.
///
/// The list is keyboard-navigable: Up/Down moves a highlighted selection
/// (persisted per-tab via `tab_id`, so multiple open launcher tabs don't
/// share selection state) and Enter launches the highlighted item, without
/// requiring the mouse. `tab_id` identifies which launcher tab this is, since
/// egui's per-frame widget memory is otherwise shared across all callers
/// within the same panel.
pub fn show_launcher(ui: &mut Ui, tab_id: TabId) -> Option<AppCommand> {
    let items = [LauncherItem {
        label: "Local Shell (platform default)",
        command: AppCommand::StartLocalSession,
    }];

    let selection_id = egui::Id::new(("launcher_selected_index", tab_id));
    let mut selected = ui
        .data(|d| d.get_temp::<usize>(selection_id))
        .unwrap_or(0)
        .min(items.len() - 1);

    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
        selected = (selected + 1) % items.len();
    }
    if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
        selected = (selected + items.len() - 1) % items.len();
    }
    let launch_via_keyboard = ui.input(|i| i.key_pressed(egui::Key::Enter));

    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Launcher");
        ui.label(
            "Start or connect to a session. Use \u{2191}/\u{2193} then Enter to launch the \
             highlighted option.",
        );
        ui.add_space(12.0);
        for (index, item) in items.iter().enumerate() {
            let response = ui.add(egui::Button::new(item.label).selected(index == selected));
            if response.clicked() {
                command = Some(item.command.clone());
            }
        }
    });

    if command.is_none() && launch_via_keyboard {
        command = Some(items[selected].command.clone());
    }

    ui.data_mut(|d| d.insert_temp(selection_id, selected));
    command
}

/// Renders the Settings application surface.
///
/// Versioned, persisted configuration (`festerm-config`) is M8 work and not
/// implemented yet. This establishes Settings as its own first-class
/// application surface with a dedicated chip today, per `docs/gui-design.md`
/// ("Settings as an application surface"): Settings never lives inside the
/// session inspector.
///
/// `chip_layout` reflects the current chip wrapping mode
/// (`docs/gui-design.md` "Wrapping must remain user-configurable"); this is
/// the one persistent preference implemented so far. Returns a command when
/// the user toggles it, dispatched through the same single command path as
/// every other invocation surface.
pub fn show_settings(ui: &mut Ui, chip_layout: ChipLayout) -> Option<AppCommand> {
    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Settings");
        ui.label(
            "Versioned, persisted configuration is not implemented yet. \
             Settings exists as its own application surface now so future \
             preferences have a stable, discoverable home.",
        );
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);
        let wrap = matches!(chip_layout, ChipLayout::Wrap);
        let label = if wrap {
            "Chip layout: wrap onto multiple rows"
        } else {
            "Chip layout: single row (scroll to see more)"
        };
        if ui.button(label).clicked() {
            command = Some(AppCommand::ToggleChipLayout);
        }
    });
    command
}
