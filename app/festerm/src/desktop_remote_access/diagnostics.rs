//! Content-free, operation-scoped setup details shared by Settings and About.
use super::HostSetupDiagnostic;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, diagnostic: Option<HostSetupDiagnostic>) {
    let Some(diagnostic) = diagnostic else { return };
    ui.group(|ui| {
        ui.label(egui::RichText::new("Last setup operation").strong());
        ui.label(format!("Stage: {}", diagnostic.stage.label()));
        if let Some(status) = diagnostic.http_status {
            ui.label(format!("HTTP status: {status}"));
        }
        ui.label(format!("Category: {}", diagnostic.category.label()));
        ui.label(format!("Next action: {}", diagnostic.recovery.label()));
        ui.label(diagnostic.recovery.guidance());
        ui.small("Operation result only. Current endpoint readiness is not verified.");
    });
}
