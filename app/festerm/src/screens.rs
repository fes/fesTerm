//! Launcher and Settings application-surface presentation.
//!
//! These are thin, product-specific screens rather than terminal chrome
//! (`crates/festerm-ui-egui/src/chrome.rs` owns the chip row). They translate
//! user gestures into `AppCommand`s per `docs/application-command-model.md`
//! and own no session or tab policy themselves; `AppState::dispatch` remains
//! the single command-handling path.

use eframe::egui::Ui;

use crate::tabs::AppCommand;

/// Renders the session launcher content and returns any dispatched command.
///
/// `docs/gui-design.md` ("Session Launcher"): fast, compact, and usable
/// repeatedly rather than a wizard or onboarding flow. SSH and saved local
/// profiles remain later milestones (M7/M8); only the local-shell path is
/// wired today, and unavailable categories are omitted rather than shown as
/// disabled clutter beyond this one illustrative placeholder.
pub fn show_launcher(ui: &mut Ui) -> Option<AppCommand> {
    let mut command = None;
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Launcher");
        ui.label("Start or connect to a session.");
        ui.add_space(12.0);
        if ui.button("Local Shell (platform default)").clicked() {
            command = Some(AppCommand::StartLocalSession);
        }
    });
    command
}

/// Renders the Settings application surface.
///
/// Versioned, persisted configuration (`festerm-config`) is M8 work and not
/// implemented yet. This establishes Settings as its own first-class
/// application surface with a dedicated chip today, per `docs/gui-design.md`
/// ("Settings as an application surface"): Settings never lives inside the
/// session inspector.
pub fn show_settings(ui: &mut Ui) {
    ui.vertical(|ui| {
        ui.add_space(24.0);
        ui.heading("Settings");
        ui.label(
            "Versioned, persisted configuration is not implemented yet. \
             Settings exists as its own application surface now so future \
             preferences have a stable, discoverable home.",
        );
    });
}
