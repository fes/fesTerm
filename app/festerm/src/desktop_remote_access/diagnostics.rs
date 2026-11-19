//! Content-free, operation-scoped setup details shared by Settings and About.
use super::HostSetupDiagnostic;
use eframe::egui;

pub(crate) fn show(
    ui: &mut egui::Ui,
    diagnostic: Option<HostSetupDiagnostic>,
    history: &[festerm_host_setup::controller::runtime_diagnostics::Record],
) {
    show_runtime_history(ui, history);
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

fn show_runtime_history(
    ui: &mut egui::Ui,
    history: &[festerm_host_setup::controller::runtime_diagnostics::Record],
) {
    use festerm_host_setup::controller::runtime_diagnostics::Correlation;
    if history.is_empty() {
        return;
    }
    ui.collapsing("Remote access runtime history", |ui| {
        ui.label("Recorded events; use Refresh local status for an updated report.");
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                for record in history {
                    let source = match record.correlation {
                        Correlation::RuntimeRecord => "Run record",
                        Correlation::ManagedHistory => "Last completed run",
                    };
                    ui.label(format!(
                        "{} · {} · {}",
                        record.timestamp,
                        record.phase.code(),
                        record.outcome.code()
                    ));
                    ui.small(format!(
                        "{source} · epoch {} · run {} · PID {}",
                        record.epoch, record.run_id, record.pid
                    ));
                    if record.outer.is_some() || record.disposition.is_some() {
                        ui.label(format!(
                            "Outer relay: {} · disposition: {}",
                            record.outer.unwrap_or("not observed"),
                            match record.disposition {
                                Some(
                                    festerm_tunnel_bridge::relay::HostFailureDisposition::Transient,
                                ) => "transient",
                                Some(
                                    festerm_tunnel_bridge::relay::HostFailureDisposition::Permanent,
                                ) => "permanent",
                                None => "not observed",
                            }
                        ));
                    }
                    if let Some(category) = record.category {
                        ui.label(format!("Category: {}", category.label()));
                    }
                    if let Some(status) = record.http_status {
                        ui.label(format!("HTTP status: {status}"));
                    }
                    if let Some(code) = record.os_code {
                        ui.label(format!("Numeric OS code: {code}"));
                    }
                    ui.separator();
                }
            });
    });
}
